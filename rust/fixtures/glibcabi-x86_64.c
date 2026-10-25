/* pid_t width, fork-style clone TID stores, creat and fadvise used by glibc tools. */
static long call(long n,long a,long b,long c,long d,long e){register long r10 __asm__("r10")=d;register long r8 __asm__("r8")=e;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10),"r"(r8):"rcx","r11","memory");return r;}
#define CHECK(x) do {if(!(x)){call(231,__LINE__,0,0,0,0);__builtin_unreachable();}} while(0)
static int parent_tid=-1,child_tid=-2;
static unsigned char affinity[4096];
__attribute__((noreturn)) void _start(void){
 long child=call(56,17|0x100000|0x1000000|0x200000,0,(long)&parent_tid,(long)&child_tid,0);
 CHECK(child>=0);
 if(!child){CHECK(child_tid==call(186,0,0,0,0,0));CHECK(parent_tid==-1);call(231,42,0,0,0,0);__builtin_unreachable();}
 CHECK(parent_tid==child && child_tid==-2);
 int status=-1;CHECK(call(61,0xfffffffful,(long)&status,0,0,0)==child && status==42*256);
 for(unsigned i=0;i<sizeof affinity;i++)affinity[i]=0xa5;
 long size=call(204,0,sizeof affinity,(long)affinity,0,0);CHECK(size>0 && size<sizeof affinity);
 unsigned any=0;for(long i=0;i<size;i++)any|=affinity[i];CHECK(any);
 for(unsigned i=size;i<sizeof affinity;i++)CHECK(affinity[i]==0xa5);
 CHECK(call(204,0,0,(long)affinity,0,0)==-22);CHECK(call(204,0,sizeof affinity,1,0,0)==-14);
 long fd=call(85,(long)"glibc-abi-file",0600,0,0,0);CHECK(fd>=0);
 CHECK(call(1,fd,(long)"abc",3,0,0)==3);
 unsigned long stat[18];CHECK(call(262,-100,(long)"glibc-abi-file",(long)stat,0x900,0)==0 && stat[6]==3);
 for(unsigned advice=0;advice<=5;advice++)CHECK(call(221,fd,0,0,advice,0)==0);
 CHECK(call(221,fd,-1,0,0,0)==0);
 CHECK(call(221,fd,0,-1,0,0)==-22);
 CHECK(call(221,fd,0,0,6,0)==-22);
 CHECK(call(221,-1,0,0,0,0)==-9);
 CHECK(call(3,fd,0,0,0,0)==0);
 fd=call(85,(long)"glibc-abi-file",0600,0,0,0);CHECK(fd>=0);
 CHECK(call(8,fd,0,2,0,0)==0);CHECK(call(3,fd,0,0,0,0)==0);
 fd=call(2,(long)"glibc-abi-file",0x900,0,0,0);CHECK(fd>=0);
 CHECK(call(72,fd,3,0,0,0)&0x800);
 long duplicate=call(32,fd,0,0,0,0);CHECK(duplicate>=0);
 CHECK(call(72,duplicate,4,0,0,0)==0);CHECK((call(72,fd,3,0,0,0)&0x800)==0);
 int nonblock=1;CHECK(call(16,fd,0x5421,(long)&nonblock,0,0)==0);CHECK(call(72,duplicate,3,0,0,0)&0x800);
 CHECK(call(3,fd,0,0,0,0)==0);CHECK(call(3,duplicate,0,0,0,0)==0);
 fd=call(257,-100,(long)".",0x90900,0,0);CHECK(fd>=0);CHECK(call(72,fd,3,0,0,0)&0x800);
 CHECK(call(72,fd,4,0,0,0)==0);CHECK((call(72,fd,3,0,0,0)&0x800)==0);CHECK(call(3,fd,0,0,0,0)==0);
 int pipe[2];CHECK(call(22,(long)pipe,0,0,0,0)==0);
 CHECK(call(221,pipe[0],0,0,2,0)==-29);
 CHECK(call(221,pipe[0],0,-1,6,0)==-29);
 CHECK(call(87,(long)"glibc-abi-file",0,0,0,0)==0);
 call(231,0,0,0,0,0);__builtin_unreachable();
}
